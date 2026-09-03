//! Agent-task command promotion source resolution and review/loop reporting tests.

use super::support::*;
use clap::Parser;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// The tests below drive the store-rooted entry points. Resolving the store
/// once here keeps the ambient lookup in one place and lets the ambient
/// wrappers be deleted (#7505).
fn test_lifecycle_store() -> homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore {
    homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
        .expect("lifecycle store")
}

#[test]
fn promotion_source_resolves_completed_run_id() {
    with_temp_home(|| {
        let run_id = "run-promotion-source";

        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(InspectingExecutor::noop(run_id)),
        )
        .expect("run completed");

        let (raw, path) = review::read_promotion_source(run_id).expect("promotion source resolved");

        assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
        assert_eq!(
            path.as_ref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str()),
            Some("aggregate.json")
        );
    });
}

#[test]
fn promotion_source_reads_bare_json_file_path() {
    let file = tempfile::NamedTempFile::new().expect("source file");
    std::fs::write(
        file.path(),
        r#"{"schema":"homeboy/agent-task-aggregate/v1"}"#,
    )
    .expect("write source");

    let (raw, path) = review::read_promotion_source(&file.path().display().to_string())
        .expect("promotion source file resolved");

    assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
    assert_eq!(path.as_deref(), Some(file.path()));
}

#[test]
fn applied_promotion_resume_requires_explicit_gate_rerun() {
    let applied = json!({ "status": "applied" });
    let failed = json!({ "status": "gate_failed" });

    assert!(!review::promotion_is_resumable(&applied, false));
    assert!(review::promotion_is_resumable(&applied, true));
    assert!(review::promotion_is_resumable(&failed, false));
}

#[test]
fn cli_promotion_resume_policy_matches_the_shared_service() {
    for (previous, rerun_completed_gates) in [
        (json!({ "status": "applied" }), false),
        (json!({ "status": "applied" }), true),
        (json!({ "status": "gate_failed" }), false),
        (json!({ "status": "verification_pending" }), false),
        (json!({ "status": "completed" }), true),
    ] {
        assert_eq!(
            review::promotion_is_resumable(&previous, rerun_completed_gates),
            homeboy::agents::agent_task_service::promotion_is_resumable(
                &previous,
                rerun_completed_gates,
            ),
        );
    }
}

#[test]
fn promotion_recipe_reference_hydrates_exact_private_gate_contract() {
    with_temp_home(|| {
        let run_id = "cook-retained-gates-attempt-1";
        let private_program = "printf 'private $TOKEN'\n";
        let gates = homeboy::agents::agent_tasks::gate::VerifyGateOptions {
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
                provider_command: None,
                provider_invocation: None,
                attempt_dispatcher: None,
            },
            gates: gates.clone(),
            retry_policy: homeboy::agents::agent_task_service::CookRetryPolicy { max_attempts: 1 },
            finalization: homeboy::agents::agent_task_service::CookFinalization {
                no_finalize: true,
                draft_pr: false,
                base: "main".to_string(),
                head: None,
                title: "Retained gates".to_string(),
                commit_message: "Retained gates".to_string(),
                protected_branches: Vec::new(),
            },
            ai_disclosure: homeboy::agents::agent_task_service::CookAiDisclosure {
                ai_tool: "fixture".to_string(),
                ai_model: None,
                ai_used_for: "test".to_string(),
            },
            harvest_context: homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()
                .expect("harvest context"),
        };
        homeboy::agents::agent_task_service::persist_initial_recipe(&options)
            .expect("persist Cook recipe");
        let cli = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "fixture",
            "--no-finalize",
        ])
        .expect("parse default gate options");
        let crate::cli_surface::Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let super::super::AgentTaskCommand::Cook(cook) = agent_task.command else {
            panic!("Cook command");
        };
        let mut cli_gates = cook.gates;

        let hydrated = review::resolve_promotion_gates(&mut cli_gates, true, Some(run_id), run_id)
            .expect("hydrate durable Cook gates");

        assert_eq!(hydrated, gates);
        assert_eq!(hydrated.private_verify, [private_program]);
        assert_eq!(hydrated.input_sources[0].path, None);
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
            provider_command: None,
            provider_argv: Vec::new(),
        })
        .expect("review loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-review/v1");
        assert_eq!(value["run_id"], "run-review-queued");
        assert_eq!(value["state"], "queued");
        assert_eq!(value["transport"]["chat_state_required"], false);
        assert!(value["aggregate_review"].is_null());
        assert_eq!(value["logs"]["events"][0]["status"], "queued");
        assert!(value["next_actions"][0]
            .as_str()
            .expect("next action")
            .contains("run-next"));
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

        let (value, exit_code) = review::review(ReviewArgs {
            run_id: "run-review-completed".to_string(),
            full: true,
            to_worktree: Some("homeboy@fix-review-flow".to_string()),
            provider_command: None,
            provider_argv: Vec::new(),
        })
        .expect("review loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["state"], "succeeded");
        assert_eq!(value["durable_read"]["phase"], "controller_local");
        assert!(value["durable_read"]["unavailable_sources"]
            .as_array()
            .expect("durable source availability")
            .is_empty());
        assert_eq!(value["aggregate_review"]["summary"]["apply_candidates"], 1);
        assert_eq!(value["artifacts"]["artifacts"][0]["id"], "patch-a");
        assert_eq!(value["promotion_candidates"][0]["task_id"], "task-a");
        assert_eq!(value["promotion_candidates"][0]["artifact_id"], "patch-a");
        assert_eq!(value["promotion_candidates"][0]["ready"], true);
        assert_eq!(
            value["promotion_candidates"][0]["command"],
            json!([
                "homeboy",
                "agent-task",
                "promote",
                "run-review-completed",
                "--task-id",
                "task-a",
                "--artifact-id",
                "patch-a",
                "--to-worktree",
                "homeboy@fix-review-flow"
            ])
        );
        assert!(value["next_actions"][0]
            .as_str()
            .expect("next action")
            .contains("promotion_candidates"));
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

        let (value, exit_code) = review::review(ReviewArgs {
            run_id: "run-review-default-bounded".to_string(),
            full: false,
            to_worktree: None,
            provider_command: None,
            provider_argv: Vec::new(),
        })
        .expect("review loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["view"], "summary");
        assert!(value.get("record").is_none());
        assert!(value.get("logs").is_none());
        assert!(value.get("artifacts").is_none());
        assert_eq!(
            value["full_command"],
            "homeboy agent-task review run-review-default-bounded --full"
        );
        assert_eq!(value["canonical_candidate"]["state"], "patch_available");
        assert_eq!(value["selected_candidate"]["size_bytes"], 42);
        assert!(value["promotion_candidates"][0]["command"].is_null());
        assert_eq!(
            value["promotion_candidates"][0]["destination_required"],
            true
        );
        let destination_guidance = value["next_actions"][0]
            .as_str()
            .expect("destination guidance");
        assert!(destination_guidance.contains(
            "homeboy agent-task review run-review-default-bounded --to-worktree <managed-worktree>"
        ));
        assert!(!destination_guidance.contains("agent-task promote"));
    });
}

#[test]
fn full_review_excludes_unrelated_worktree_cleanup_inventory() {
    with_temp_home(|| {
        let run_id = "run-review-scoped-cleanup";
        run_loaded_plan(test_plan(), Some(run_id), Arc::new(ApplyArtifactExecutor))
            .expect("run completed");
        let unrelated_worktrees = (0..59)
            .map(|index| format!("/workspace/unrelated-worktree-{index}"))
            .collect::<Vec<_>>();
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["automatic_artifact_retention"] = json!({
                "status": "completed",
                "worktree_count": unrelated_worktrees.len(),
                "worktrees": unrelated_worktrees,
            });
        })
        .expect("persist unrelated cleanup inventory");

        let (review_value, exit_code) = review::review(ReviewArgs {
            run_id: run_id.to_string(),
            full: true,
            to_worktree: None,
            provider_command: None,
            provider_argv: Vec::new(),
        })
        .expect("review loaded");

        assert_eq!(exit_code, 0);
        assert!(review_value["record"]["metadata"]
            .get("automatic_artifact_retention")
            .is_none());
        assert_eq!(
            review_value["cleanup_evidence"][0]["kind"],
            "automatic_artifact_retention"
        );
        assert_eq!(
            review_value["cleanup_evidence"][0]["command"],
            format!("homeboy agent-task status {run_id}")
        );
        assert_eq!(
            review_value["cleanup_evidence"][0]["export_command"],
            format!("homeboy agent-task status {run_id} --output <path>")
        );
        assert!(!review_value.to_string().contains("unrelated-worktree-58"));
        let persisted =
            agent_task_lifecycle::reconcile_status(run_id).expect("cleanup evidence persists");
        assert_eq!(
            persisted.metadata["automatic_artifact_retention"]["worktrees"]
                .as_array()
                .map(Vec::len),
            Some(59)
        );
    });
}

#[test]
fn cook_readers_keep_the_substantive_candidate_after_a_no_change_retry() {
    struct PatchExecutor {
        path: String,
        run_id: String,
        size_bytes: u64,
        sha256: String,
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
            AgentTaskOutcome {
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("produced patch before failed gate".to_string()),
                artifacts: vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "candidate".to_string(),
                    kind: "patch".to_string(),
                    name: Some("candidate.patch".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some(self.path.clone()),
                    url: None,
                    mime: Some("text/x-diff".to_string()),
                    size_bytes: Some(self.size_bytes),
                    sha256: Some(self.sha256.clone()),
                    metadata: Value::Null,
                }],
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
        let patch = tempfile::NamedTempFile::new().expect("candidate patch");
        let patch_contents = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";
        std::fs::write(patch.path(), patch_contents).expect("write candidate patch");
        run_loaded_plan(
            test_plan(),
            Some(candidate_run_id),
            Arc::new(PatchExecutor {
                path: patch.path().display().to_string(),
                run_id: candidate_run_id.to_string(),
                size_bytes: patch_contents.len() as u64,
                sha256: homeboy_engine_primitives::content_hash::sha256_hex(
                    patch_contents.as_bytes(),
                ),
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
                "provenance": { "gate_retry": "intentional_no_change" }
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
            provider_command: None,
            provider_argv: Vec::new(),
        })
        .expect("Cook review is bounded");
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
        for value in [&review_value, &diagnose_value, &evidence_value] {
            assert_eq!(value["run_id"], candidate_run_id);
            assert_eq!(
                value["candidate_selection"]["latest_attempt_run_id"],
                retry_run_id
            );
            assert_eq!(value["candidate_selection"]["run_id"], candidate_run_id);
        }
        assert_eq!(review_value["contributing_attempt"]["run_id"], retry_run_id);
        assert_eq!(
            review_value["contributing_attempt"]["review_form"]["summary"],
            "Retry reviewed the candidate."
        );
        assert_eq!(
            review_value["contributing_attempt"]["verification"]["provenance"]["gate_retry"],
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

#[cfg(unix)]
#[test]
fn direct_cook_promotion_resolves_recovered_patch_from_run_id_and_aggregate_path() {
    with_temp_home(|| {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        std::fs::create_dir(&source).expect("create source");
        init_runtime_component_checkout(&source);
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "selected-large-patch",
                target.to_str().expect("target path"),
                "main",
            ])
            .current_dir(&source)
            .status()
            .expect("create target worktree")
            .success());

        let cook_id = "cook-direct-selected-large-patch";
        let candidate_run_id = "cook-direct-selected-large-patch-attempt-1";
        let retry_run_id = "cook-direct-selected-large-patch-attempt-2";
        let task_id = "provider";
        let artifact_id = "cook-homeboy-attempt-1-uncommitted-changes";
        let plan = AgentTaskPlan::new(
            "cook-direct-selected-large-patch-plan",
            vec![serde_json::from_value(json!({
                "task_id": task_id,
                "executor": {"backend": "fixture", "model": "fixture-model"},
                "instructions": "produce a retained large patch",
                "workspace": {"root": source},
            }))
            .expect("candidate task")],
        );
        let options = homeboy::agents::agent_task_service::CookRequest {
            identity: homeboy::agents::agent_task_service::CookIdentity {
                cook_id: cook_id.to_string(),
                initial_run_id: candidate_run_id.to_string(),
                initial_plan: plan.clone(),
            },
            workspace: homeboy::agents::agent_task_service::CookWorkspace {
                to_worktree: "fixture@selected-large-patch".to_string(),
                source_worktree_path: Some(source.clone()),
                task_base_sha: None,
                source_refs: Vec::new(),
            },
            provider_transport: homeboy::agents::agent_task_service::CookProviderTransport {
                provider_command: None,
                provider_invocation: None,
                attempt_dispatcher: None,
            },
            gates: Default::default(),
            retry_policy: homeboy::agents::agent_task_service::CookRetryPolicy { max_attempts: 2 },
            finalization: homeboy::agents::agent_task_service::CookFinalization {
                no_finalize: true,
                draft_pr: false,
                base: "main".to_string(),
                head: None,
                title: "selected large patch".to_string(),
                commit_message: "selected large patch".to_string(),
                protected_branches: Vec::new(),
            },
            ai_disclosure: homeboy::agents::agent_task_service::CookAiDisclosure {
                ai_tool: "fixture".to_string(),
                ai_model: Some("fixture-model".to_string()),
                ai_used_for: "test".to_string(),
            },
            harvest_context: homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()
                .expect("harvest context"),
        };
        homeboy::agents::agent_task_service::persist_initial_recipe(&options)
            .expect("persist Cook recipe");
        agent_task_lifecycle::submit_plan(&plan, Some(candidate_run_id)).expect("submit candidate");

        let patch = format!(
            "diff --git a/large.txt b/large.txt\nnew file mode 100644\nindex 0000000..e69de29\n--- /dev/null\n+++ b/large.txt\n@@ -0,0 +1 @@\n+{}\n",
            "x".repeat(300 * 1024),
        );
        assert!(patch.len() > 256 * 1024);
        let patch_sha256 = homeboy_engine_primitives::content_hash::sha256_hex(patch.as_bytes());
        let artifact = AgentTaskArtifact {
            id: artifact_id.to_string(),
            kind: "patch".to_string(),
            path: Some("runner-artifact://expired/large.patch".to_string()),
            size_bytes: Some(patch.len() as u64),
            sha256: Some(patch_sha256.clone()),
            metadata: json!({
                "run_id": candidate_run_id,
                "task_id": task_id,
                "producer_attempt": 1,
                "base_ref": "main",
                "provider_backend": "fixture",
                "provider_model": "fixture-model",
                "repository_identity": "fixture",
                "workspace_identity": "fixture",
            }),
            ..Default::default()
        };
        agent_task_lifecycle::record_run_aggregate(
            candidate_run_id,
            &plan,
            &AgentTaskAggregate {
                schema: "homeboy/agent-task-aggregate/v1".to_string(),
                plan_id: plan.plan_id.clone(),
                status: homeboy::agents::agent_tasks::scheduler::AgentTaskAggregateStatus::CandidateRecoverable,
                totals: Default::default(),
                outcomes: vec![AgentTaskOutcome {
                    schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: task_id.to_string(),
                    status: AgentTaskOutcomeStatus::CandidateRecoverable,
                    artifacts: vec![artifact],
                    metadata: json!({"model": "fixture-model"}),
                    ..Default::default()
                }],
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            },
        )
        .expect("record recoverable candidate");

        let observation_store = homeboy::core::observation::ObservationStore::open_initialized()
            .expect("observation store");
        let retained = observation_store
            .artifact_root()
            .expect("artifact root")
            .join("direct-cook-selected-large-patch.patch");
        std::fs::create_dir_all(retained.parent().expect("retained patch parent"))
            .expect("create retained patch parent");
        std::fs::write(&retained, &patch).expect("retain patch under controller ownership");
        observation_store
            .record_verified_artifact_with_id(
                candidate_run_id,
                "patch",
                &retained,
                "direct-cook-selected-large-patch",
                Some(patch.len() as i64),
                Some(&patch_sha256),
                json!({"agent_task": {
                    "projection": "controller_local",
                    "task_id": task_id,
                    "logical_artifact_id": artifact_id,
                }}),
            )
            .expect("record retained patch projection");

        let retry_plan = AgentTaskPlan::new(
            "cook-direct-selected-large-patch-retry-plan",
            vec![serde_json::from_value(json!({
                "task_id": "review",
                "executor": {"backend": "fixture", "model": "fixture-model"},
                "instructions": "review without changing the candidate",
                "workspace": {"root": source},
            }))
            .expect("retry task")],
        );
        agent_task_lifecycle::submit_plan(&retry_plan, Some(retry_run_id)).expect("submit retry");
        agent_task_lifecycle::record_run_aggregate(
            retry_run_id,
            &retry_plan,
            &AgentTaskAggregate {
                schema: "homeboy/agent-task-aggregate/v1".to_string(),
                plan_id: retry_plan.plan_id.clone(),
                status:
                    homeboy::agents::agent_tasks::scheduler::AgentTaskAggregateStatus::Succeeded,
                totals: Default::default(),
                outcomes: vec![AgentTaskOutcome {
                    schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: "review".to_string(),
                    status: AgentTaskOutcomeStatus::Succeeded,
                    outputs: json!({"review_form": {
                        "summary": "The retained candidate is ready.",
                        "what_changed": ["No additional patch was needed."],
                        "compatibility": "No additional impact.",
                        "used_for": "Reviewed the retained candidate."
                    }}),
                    ..Default::default()
                }],
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            },
        )
        .expect("record no-change retry");
        agent_task_lifecycle::record_cook_attempt_in_store(
            &test_lifecycle_store(),
            cook_id,
            1,
            candidate_run_id,
        )
        .expect("index candidate attempt");
        agent_task_lifecycle::record_cook_attempt_in_store(
            &test_lifecycle_store(),
            cook_id,
            2,
            retry_run_id,
        )
        .expect("index latest retry");
        agent_task_lifecycle::record_promotion(
            retry_run_id,
            json!({
                "status": "gate_failed",
                "gate_results": [{"name": "fixture", "exit_code": 1}],
                "provenance": {"gate_retry": "intentional_no_change"},
            }),
        )
        .expect("record retry provenance");

        let provider = temp.path().join("promotion-provider.sh");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '%s\\n' '{{\"schema\":\"homeboy/agent-task-promotion-apply-response/v1\",\"workspace_path\":\"{}\"}}'\n",
                target.display(),
            ),
        )
        .expect("write promotion provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");

        agent_task_lifecycle::materialize_recovered_patch_artifact(
            candidate_run_id,
            Some(task_id),
            Some(artifact_id),
        )
        .expect("recover retained patch into the canonical aggregate");
        let (run_source, aggregate_path) =
            review::read_promotion_source(candidate_run_id).expect("read run source");
        let aggregate_path = aggregate_path.expect("canonical aggregate path");
        let (path_source, _) = review::read_promotion_source(&aggregate_path.display().to_string())
            .expect("read exact aggregate source");
        assert_eq!(run_source, path_source);

        let mut reports = Vec::new();
        for source_spec in [
            candidate_run_id.to_string(),
            aggregate_path.display().to_string(),
        ] {
            let cli = crate::cli_surface::Cli::try_parse_from([
                "homeboy",
                "agent-task",
                "promote",
                &source_spec,
                "--artifact-id",
                artifact_id,
                "--to-worktree",
                "fixture@selected-large-patch",
                "--provider-argv",
                "sh",
                "--provider-argv",
                provider.to_str().expect("provider path"),
                "--dry-run",
                "--gates-from-cook-recipe",
            ])
            .expect("parse public direct promotion command");
            let crate::cli_surface::Commands::AgentTask(agent_task) = cli.command else {
                panic!("agent-task command");
            };
            let super::super::AgentTaskCommand::Promote(args) = agent_task.command else {
                panic!("promote command");
            };
            let (report, exit_code) = review::promote_artifact(*args)
                .expect("recovered candidate remains directly promotable");

            assert_eq!(exit_code, 0);
            assert_eq!(report["status"], "dry_run");
            assert_eq!(report["source"]["run_id"], candidate_run_id);
            assert_eq!(report["patch_artifact"]["id"], artifact_id);
            reports.push(report);
        }
        assert_eq!(reports[0]["patch_artifact"], reports[1]["patch_artifact"]);
    });
}
